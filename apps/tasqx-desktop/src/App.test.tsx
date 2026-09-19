import { render, screen } from '@testing-library/react';

import { App } from './App';

test('renders the placeholder root', () => {
  render(<App />);
  expect(screen.getByTestId('app')).toHaveTextContent('Tasqx Desktop');
});
